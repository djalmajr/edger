# Banco SQLD gerenciado (EdgeR)

**Status:** implementado no PR #179 (EDG-52 a EDG-54), ainda sem release. Este
documento descreve o código; onde o contrato de desenho
(`.agents/plans/edger-sqld-provisioning-contract.md`) diverge, vale o código. Não
há, aqui, afirmação de que o recurso foi publicado ou implantado em algum
ambiente.

## O que "gerenciado" quer dizer

O EdgeR **gerencia namespaces de um `sqld` externo** pelas APIs HTTP de admin e de
dados do próprio `sqld`. O processo `edger` **não inicia, reinicia nem supervisiona
um `sqld`**: não existe esse código no binário. Existe um único modo de operação,
ligado ou desligado por variáveis de ambiente:

| Variáveis `EDGER_SQLD_*` presentes | Resultado |
|---|---|
| Nenhuma | Serviço desligado. As rotas de banco respondem `503 SQLD_UNAVAILABLE`. Nenhuma concessão de banco é criada. |
| As sete, todas válidas | Serviço ligado contra o `sqld` externo. |
| Algumas, ou alguma vazia/inválida | O processo **não sobe** (erro fatal no boot). Nunca cai para "desligado". |

O contrato falava em "configuração parcial não provisiona"; o código vai além e
recusa o boot. O boot também exige o cofre de segredos (`EDGER_SECRETS_KEY_FILE`)
e, no Unix, é o único suportado: fora do Unix, configurado dá erro estático. O
boot não faz nenhuma chamada de rede: o probe e o `CREATE` acontecem no `PUT`.

O `sqld` precisa ser iniciado, mantido e dimensionado pelo operador (o repositório
traz um exemplo em `deploy/production/sqld.yaml`, fora do chart). Os testes que
iniciam um `sqld` real usam `EDGER_SQLD_BIN`, uma variável **só de teste**.

## Configuração do operador

Sete variáveis, sem valor padrão, lidas no boot
(`crates/edger-orchestrator/src/sqld_managed_config.rs`):

| Variável | Regra |
|---|---|
| `EDGER_SQLD_ADMIN_URL` | Origem do admin do `sqld` (`http` ou `https`, sem userinfo, caminho, query nem fragmento). |
| `EDGER_SQLD_ADMIN_KEY_FILE` | Caminho absoluto da chave admin. Arquivo regular privado, até 4096 bytes. O conteúdo recebe `trim` ASCII uma vez e precisa ter só bytes `0x21..0x7e` (sem espaço interno, porque o `sqld` 0.24.32 compara só o segundo token após `Basic`). |
| `EDGER_SQLD_USER_URL` | Origem da porta de dados. Precisa ser `http`, com host, caminho `/`, porta diferente de 9090 e **origem diferente** da admin. |
| `EDGER_SQLD_REGISTRY_DB` | Caminho absoluto do ledger SQLite. O diretório pai já deve existir (`0700`, do euid). O arquivo é criado `0600`. |
| `EDGER_SQLD_SIGNER_KEY_FILE` | Master do signer: 32 bytes crus ou base64 de exatamente 32 bytes (leitura de até 256 bytes). **Deve ser diferente** da master do cofre. |
| `EDGER_SQLD_MANAGED_BUDGET_BYTES` | Orçamento total de reserva, só dígitos decimais, no mínimo 16 MiB, múltiplo de 4096. Fica gravado no ledger; um valor diferente no próximo boot dá `BudgetMismatch`. |
| `EDGER_SQLD_BACKUP_DIR` | Diretório já existente, caminho absoluto, modo `0700`, do euid. |

Regras dos arquivos de chave: regular, dono root ou o euid, sem setuid, setgid ou
sticky, sem bits para "others", sem escrita ou execução de grupo, sem execução do
dono; leitura de grupo só se o gid for o do processo. Elas ficam **fora de toda
raiz que o guest lê** (diretórios de workers, core, overlay, cache do Deno e
`EDGER_DENO_CACHE_ROOT`). O código só confere o estado visto naquele momento.

Dependências já existentes: `EDGER_SECRETS_KEY_FILE` e `EDGER_APP_SECRETS_DB` (o
cofre), `ROOT_API_KEY` e a autenticação do plano de controle para as rotas
admin, e `EDGER_DENO_BIN` quando há guest Deno.

## API de administração

Três rotas, só para o **root** (credencial real, nunca o atalho do modo aberto),
em `crates/edger-orchestrator/src/admin_api.rs`. Toda resposta, inclusive erro,
leva `Cache-Control: no-store`. Não há rota `DELETE` nem de restauração. O worker
precisa estar instalado e não ser do core.

| Rota | Corpo | Resposta |
|---|---|---|
| `GET /api/admin/workers/{name}/database` | nenhum | `{"database": null ou {state, sqlNamespace, quotaBytes}, "activation": "nextSpawn"}` |
| `PUT /api/admin/workers/{name}/database` | `{"quotaBytes": <u64>}` | `{"database": {...}, "activation": "nextSpawn", "initialBackup": null ou {fileName, bytes, sha256, retention}}` |
| `POST /api/admin/workers/{name}/database/backup` | vazio ou `{}` | `{"backup": {fileName, bytes, sha256, retention: "complete" ou "incomplete"}}` |

Nenhuma resposta traz JWT ou chave. O corpo é limitado a 1024 bytes, lido só
depois da autenticação (413 acima disso); o `PUT` recusa campo desconhecido (400).
Qualquer query string dá 400 `INVALID_SQLD_REQUEST`.

Ordem das checagens: credencial real (401) e troca de senha pendente (403),
root (403), CSRF (403 `CSRF_DENIED`), query (400), serviço configurado (503),
worker do core (403) antes de worker ausente (404), corpo.

`quotaBytes`: no mínimo 16 MiB, múltiplo de 4096, no máximo o orçamento do
ledger. Valores inaceitáveis dão 400.

### Estados

| Estado | Significado |
|---|---|
| `intent` | Linha, chave selada e reserva gravadas; sem prova remota. |
| `proven` | A chave pública remota do namespace é a gravada. |
| `ready` | Token emitido, quota provada e backup inicial feito. |
| `conflict` | Terminal, sem caminho de volta: chave remota ausente, inválida ou diferente. |

### Sequência do `PUT`

1. Valida a quota e a disponibilidade do cofre; toma o lock do owner.
2. Um token `SQLD_TOKEN` já presente no cofre (mesmo vazio) para um app novo ou em
   `intent` dá `409 SQLD_CONFLICT`, sem probe, reserva nem `CREATE`.
3. Probe do admin: a chamada anônima precisa dar 401 e a autenticada, 200 ou 404;
   senão o admin é recusado (`ApiOpen`/`Unauthorized`, 503).
4. Grava a intenção (linha, chave Ed25519 selada com ChaCha20-Poly1305 e reserva)
   numa transação `BEGIN IMMEDIATE`.
5. Prova remota: `GET` da config do namespace; se ausente e em `intent`, `CREATE`
   (sempre com `jwt_key`, `max_db_size` e `allow_attach:false`) e novo `GET`. Um
   `CREATE` que devolve 200 ou 400 não prova nada: só o `GET` seguinte conta.
   Namespace ausente em `proven` ou `ready` dá 409 e **nunca recria** um banco vazio.
6. Semeia ou renova o token. 7. `PRAGMA max_page_count` precisa ser `quota/4096`
   (senão 409 `QuotaMismatch`). 8. Em `proven`, faz o backup inicial e marca `ready`.

Repetir o `PUT` com a mesma quota num app `ready` devolve 200 com
`initialBackup: null`. Quota diferente dá 409. Todas as versões instaladas do
nome são conferidas antes do `ensure`: conflito de `SQLD_URL` diferente da origem
do host (em `env` ou em `secrets:`), `SQLD_TOKEN` em `env`/`publicEnv` ou versão
Deno sem `allowNet: []` dão 409 sem reservar nada. A ativação vale no **próximo
spawn**: `activation: "nextSpawn"`; processos vivos não são tocados (use o
`recycle` do worker para aplicar na hora).

### Códigos de erro

- `409 SQLD_CONFLICT`: token manual, token ausente, quota diferente, orçamento
  estourado, chave em conflito, namespace remoto ausente, estado não `ready`,
  `QuotaMismatch`, colisão de namespace e as demais transições inválidas do ledger.
- `503 SQLD_UNAVAILABLE`: serviço não configurado, cofre indisponível, qualquer
  falha remota (admin, dados, backup), falha de integridade ou armazenamento do
  ledger, e esgotamento de admissão (15 s).
- `404 NOT_FOUND`, `403 FORBIDDEN`, `400 INVALID_SQLD_REQUEST`, `401`, `413`.

## Quotas e limites

| Limite | Valor |
|---|---|
| Bancos por app | 1 (por nome integral e namespace RBAC); todas as versões do app usam o mesmo |
| Quota por app | mínimo 16 MiB, múltiplo de 4096, imutável, até o orçamento |
| Reserva total | soma de todas as linhas (inclusive `intent` e `conflict`) até o orçamento; **nada é liberado** |
| Imposição no `sqld` | `max_db_size` no `CREATE` e `max_page_count` provado no `PUT` |
| Não limitado pelo EdgeR | linhas, conexões, WAL, temporários, backups e apps não gerenciados |
| Cliente admin | 15 s no total, 5 s de conexão, 10 s de leitura, resposta até 64 KiB, sem redirect, sem retry, sem proxy |
| Pedido do app ao banco (guard) | sem `egress` no manifest: resposta até 100 MiB, 8 concorrentes por worker e versão, 5 s conexão, 30 s cabeçalho, 30 s ocioso, 300 s total |

Quota de WAL, temporários ou volume **não** é garantida; deixe margem no volume
do `sqld`. Medida no ensaio com orçamento de 5 GiB (`sqld` 0.24.32 real, binário
real, 2 + 2 + 1 GiB de quotas, um app de 1 GiB preenchido até `SQLITE_FULL`): os
1.071.644.672 bytes aceitos pelo app ocuparam cerca de 2,17 GB no diretório do
`sqld` (banco, WAL e temporários). Planeje o volume com pelo menos o dobro do que
os apps gravam, mais os backups. Não houve teste com um PVC de 5 GiB em cluster. O ledger e a master do signer não entram em nenhum backup de app: o
ledger selado exige backup consistente separado e a mesma master para ser lido.

## Backup

- O que: `GET /dump` na porta de dados do `sqld`, autorizado com o token do próprio
  app e `x-namespace-bin`. A saída é um dump SQL em texto.
- Onde: `EDGER_SQLD_BACKUP_DIR`. Arquivos `<namespace>-<uuid>.sql`, modo `0600`,
  escritos em `.tmp-<uuid>`, `fsync` e publicados por `linkat` (nunca sobrescreve).
  Um pedido cancelado não deixa arquivo parcial.
- Quando: uma vez, como backup inicial na passagem `proven` -> `ready`; e sob
  demanda por `POST .../database/backup` (exige `ready`, prova o namespace remoto
  antes e renova o token se preciso). **Não existe backup agendado** e não existe
  rota nem código de restauração; o teste `backup_is_private_complete_and_restorable`
  só prova que o dump é restaurável.
- Retenção: fixa em 3 cópias por namespace. Falha na limpeza devolve
  `retention: "incomplete"` e mantém o arquivo novo.
- Limites do dump: 256 MiB, conexão de 5 s, leitura de 30 s e 1800 s no total.
  São constantes do código, **sem variável de ambiente**.
- **Limite conhecido (EDG-87):** o teto vale para o dump SQL serializado, não para
  os bytes gravados nem para a quota, e a quota pode ser maior que ele. Quando o
  dump de um app passa de 256 MiB, esta rota não gera backup novo; a falha acontece
  antes da publicação e as cópias anteriores continuam guardadas (a retenção só
  roda depois de uma publicação bem-sucedida). No ensaio, o backup de um app com
  1 GiB preenchido produziu o dump a cerca de 0,19 MB/s e não respondeu em 600 s.
  Estimativa, não observada: nesse ritmo o teto seria alcançado em cerca de 23
  minutos, e o pedido terminaria em `503 SQLD_UNAVAILABLE`, sem dizer a causa.
  Durante o backup, outro app continuou respondendo às consultas e às rotas admin.
  A correção está no card EDG-87.

## Token e renovação

- Cada app tem uma chave Ed25519 própria, gerada uma vez e selada sob a master do
  signer. O JWT é EdDSA com claims exatos `{exp, p: {rw: {ns: [ns]}, ddl: {ns: [ns]}}}`
  e **vida de 12 h**; é renovado quando restam menos de 1 h (ou já expirou).
- O token vai ao cofre do app com o nome `SQLD_TOKEN`, por compare-and-set: a
  semeadura exige ausência e a renovação exige igualdade com o valor lido. Um `PUT`
  manual concorrente vence e é preservado (o lado perdedor vê `TokenConflict`, 409).
  Um valor manual ou inválido no cofre nunca é substituído. Antes de confiar em
  qualquer token, o host confere assinatura, claims e `exp`.
- Rotação da **chave de assinatura não existe**: a geração é fixa em 1 e não há
  rota de rekey. Diferença de chave remota é `conflict` terminal.
- O app recebe `SQLD_URL` (a origem de dados do host, num clone da configuração) e
  o nome `SQLD_TOKEN` entre os segredos; o valor nunca entra em `WorkerConfig`.
  Deno recebe o segredo no `env` do processo no spawn; WASM, no contexto WASI de
  cada execução.
- A autorização real de cada chamada ao banco não é o token do guest: o guard do
  host remove `Authorization`, `Cookie`, `Proxy-Authorization` e `x-namespace-bin`
  do guest e impõe os dele, e a cada chamada exige o estado `ready` e usa o token
  verificado e renovado pelo resolvedor.

Limites da prova da renovação:

- **Por teste de componente** (`managed_database_egress.rs`, `sqld` e Deno reais,
  sink armado à mão, relógio controlado no nível do serviço): durante a vida de um
  processo, o token do `env` do Deno fica velho e o guard usa o token novo do
  cofre, no mesmo PID e num único socket.
- **Pelo binário `edger`**, há prova da renovação no próximo spawn (após `recycle`)
  e também **durante a vida do mesmo processo Deno**: o teste
  `own_token_is_renewed_while_the_same_deno_process_keeps_its_old_env` grava no cofre
  um token próprio perto de vencer e depois já vencido; a consulta seguinte, no
  mesmo processo (mesmo `id` em `/metrics/stats` e mesmo PID), funciona, o cofre
  recebe um token com `exp` posterior e o `env` do processo continua com o token
  antigo. Um proxy de teste entre o `edger` e a porta de dados guarda só o hash do
  `Authorization` de cada pedido: a consulta inicial chega ao `sqld` com o token
  original e cada consulta depois da renovação chega com o token novo do cofre,
  diferente do original. Com um token manual no cofre, a consulta falha antes de
  chegar ao `sqld` e o token não é trocado.
  O binário não expõe relógio: o teste troca o token, não espera as 12 h.

## Rede

- **Deno:** o spawn exige `allowNet: []`. O bootstrap captura o `fetch` nativo antes
  da app; chamadas cuja origem normalizada é a do banco vão, por um cliente HTTP em
  socket Unix, ao guard; o resto usa o `fetch` nativo (o wrapper não é fronteira
  de segurança, a fronteira é `allowNet: []`). Falha ao abrir o socket recusa o spawn.
  O modo bridge (`EDGER_JS_RUNTIME=bridge`) recusa execução se houver qualquer
  registro de banco (`SQLD_MANAGED_RUNTIME_UNSUPPORTED`).
- **WASM:** o import `edger_http_v1.fetch` (veja [wasm-abi.md](wasm-abi.md)). Sockets
  brutos continuam negados.
- **Regras do guard:** só `http` para a origem de dados configurada, porta 9090
  recusada, sem userinfo nem fragmento, nome DNS resolvido como absoluto e o
  primeiro IP fixado, qualquer 3xx recusado, header de namespace
  `x-namespace-bin` = base64 (sem padding) de `edger_<64 hex>` derivado de
  SHA-256(worker, namespace RBAC). Não há classificador de IP privado nesse caminho:
  a concessão do host é que admite uma origem privada ou loopback. Recusas usam
  `EGRESS_DATABASE_DENIED` ou `EGRESS_DATABASE_AUTH_UNAVAILABLE`.
- Outros destinos: só se o manifest declarou `egress`; a origem do banco nunca cai
  no caminho geral.
- O `sqld` de exemplo em `deploy/production/sqld.yaml` mantém o admin em
  `127.0.0.1:9090` dentro do pod e uma NetworkPolicy `sqld-ingress` só na porta
  8080. O pod do `edger` **não alcança o admin** assim como está: `EDGER_SQLD_ADMIN_URL`
  exige um endereço de admin alcançável, o que esses manifestos não fornecem.

## Duas instâncias disputando o mesmo app

O código não bloqueia nenhuma: o ledger serializa as duas em `BEGIN IMMEDIATE`
(uma linha, uma chave, uma reserva), as duas podem tentar o `CREATE` (um dá 200 e
o outro 400) e as duas leem a mesma chave pública no `GET` seguinte. Quem perde o
compare-and-set na semeadura do token recebe `409 SQLD_CONFLICT` e, ao repetir,
lê o token válido do outro como `Fresh` e recebe 200. Portanto o `PUT` concorrente
pode devolver 200 ou um 409 transitório; repetir converge para 200.

Limite da prova: o teste de binário com dois processos
(`two_edger_processes_race_to_provision_the_same_app`) aceita 200 ou 409 e só roda
a asserção "409 estável e depois 200" quando um 409 de fato ocorre. **Nas rodadas
executadas, ambos deram 200: esse ramo não foi exercitado no binário.** O ramo
"perdeu o compare-and-set" é coberto de forma determinística só no nível do serviço
(`concurrent_manual_put_inside_the_seed_window_wins_over_ensure`). A disputa pelo
último espaço do orçamento (`...race_for_the_last_budget_slot`) espera exatamente
`[200, 409]` e 409 estável na repetição. Um ledger local não coordena hosts com
arquivos distintos.

## Chart e operação

O chart `charts/edger/` **não expõe** nenhuma das sete variáveis `EDGER_SQLD_*`,
não monta volumes para o ledger, as chaves e os backups, e não inclui `sqld`. Na
prática, em Kubernetes hoje, o operador teria que injetar as variáveis por
`extraEnv`/`extraEnvFrom` e montar os arquivos e diretórios `0700` por conta
própria; isso é uma lacuna do chart, não um procedimento suportado. Veja
[deployment-k8s.md](deployment-k8s.md).

Há uma segunda lacuna, de operação: `deploy/production/sqld.yaml` mantém o admin
do `sqld` em loopback (`--admin-listen-addr=127.0.0.1:9090`, dentro do pod do
`sqld`), portanto o pod do `edger` **não consegue** alcançar o endereço que
`EDGER_SQLD_ADMIN_URL` exige. Hoje nem o chart configura o banco gerenciado nem os
manifestos de produção o tornam alcançável. As duas são lacunas conhecidas; este
documento não promete data para fechá-las.

## Provas e o que falta

Provado por testes: serviço (relógio controlado, `sqld` simulado), componentes com
`sqld` real (Deno e Go/WASM reais, dois apps, tokens e namespaces cruzados, quota
cheia, dump restaurável) e o binário `edger` com `sqld` e Deno reais
(`sqld_managed_cli_e2e.rs`, `#[ignore]`, exige `EDGER_SQLD_BIN` e `EDGER_DENO_BIN`):
isolamento entre apps, quota cheia, crashes entre `CREATE` e `ready`, namespace
alheio como conflito, renovação no próximo spawn e durante a vida do mesmo processo
Deno (`own_token_is_renewed_while_the_same_deno_process_keeps_its_old_env`), o
ensaio com orçamento de 5 GiB (`five_gib_budget_and_one_gib_guest_fill_preserve_other_apps`)
e as disputas acima. Não provado: o ramo "409 estável e depois 200" da disputa de
dois processos, o backup de um banco maior que o teto do dump (ver Backup) e
qualquer instalação em cluster.
