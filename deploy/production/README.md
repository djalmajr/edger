# Plataforma de produção do EdgeR (`deploy/production/`)

A instância do EdgeR sai do namespace `cinzel` para o namespace genérico
`edger` e passa a servir vários apps (Cinzel, Pinar prd/stg e os
próximos) com **um sqld compartilhado**: um banco (namespace do libsql)
por app e ambiente. Aprovado no plano
`plano-edger-namespace-sqld-20261014.md` (revisado pelo Cinzel em
2026-10-14; adendo EDG-27 do Pinar no mesmo dia). A ordem da janela de
corte e o rollback abaixo seguem a correção da revisão de manifests
(Emenda 1, F1/F2, e Emenda 2 — barreira de quiescência no rollback,
todas de 2026-10-15): **quiescência antes do dump final** e rollback em
dois casos, cada um atrás da barreira de quiescência — o arquivo do
plano (do orquestrador) mantém a ordem original e não foi editado.

Este diretório versiona **a plataforma** (release `edger`, sqld, backup,
rota do control plane). **Rotas e segredos de cada app ficam nos
repositórios dos apps** — Cinzel: `deploy/k8s/` do repositório do Cinzel
+ OpenBao `secret/cinzel/prod/database`; Pinar: repositório do Pinar.
A remoção de `deploy/edger`, `deploy/k8s/cinzel-sqld.yaml` e
`deploy/k8s/backup` do repositório do Cinzel só acontece **depois** do
corte, num PR deles (revisão do Cinzel, item 8).

## Desenho

| Peça | Nome | Observação |
|---|---|---|
| EdgeR | release `edger`, `fullnameOverride: edger`, Service `edger:3000`, PVC `edger-user-workers` (local-path, 5Gi) | chart `charts/edger`, `rc.19` ou mais nova (com a EDG-24); request 1 Gi / limite 4 Gi de memória |
| sqld | Deployment `sqld`, PVC `sqld-data` (local-path, 5Gi), `--enable-namespaces`, admin em `127.0.0.1:9090` com `--admin-auth-key` (via `LIBSQL_ADMIN_AUTH_KEY`), `--auth-jwt-key-file` (chave global só para o `default`) | 1 réplica, `Recreate`, tag `v0.24.32` fixada |
| Bancos | namespaces do libsql `cinzel`, `pinar-prd`, `pinar-stg`, cada um com `jwt_key` própria | o `default` nasce vazio; depois do corte e dos 7 dias de rollback, `--disable-default-namespace` |
| Hosts dos bancos | Services `cinzel`, `pinar-prd`, `pinar-stg` no namespace `edger`, todos com o seletor do pod `sqld`, porta 8080 | o cliente escolhe o banco pelo **primeiro rótulo do Host**: `http://cinzel.edger.svc.cluster.local:8080` → banco `cinzel`. Host sem ponto cai no `default` — nunca use o nome curto do Service como URL |
| NetworkPolicy | `sqld-ingress`: entrada na 8080 só dos pods do EdgeR (`app.kubernetes.io/instance: edger`) e dos jobs de backup (`app.kubernetes.io/name: sqld-backup`) | `kubectl port-forward` vai pelo kubelet e não é afetado |
| Backup | um CronJob por banco (`sqld-backup-<banco>`), retenção/sentinela/token por banco, PVC `sqld-backups` compartilhado | restaurar um banco **não** para o EdgeR: só o app afetado sai do ar, com o worker desabilitado |
| Rotas e TLS | IngressRoute `edger` (`edger.djalmajr.dev`, **só control plane** — adendo EDG-27), Middleware `edger-admin-ratelimit`, Certificate `edger-tls` (`letsencrypt-dns`), tudo em `edger` | as rotas dos apps (`cinzel.app`, `pinar.dev`, …) vivem nos repositórios dos apps e também são aplicadas em `edger` (o Traefik não tem `allowCrossNamespace`) |
| Secrets | `edger-root`, `sqld-jwt`, `sqld-admin`, `sqld-jwt-<banco>`, `sqld-backup-<banco>` | nenhum valor em git; nada de `latest` em imagem |

## Inventário

| Objeto | Kind | Nome | Arquivo |
|---|---|---|---|
| Namespace da plataforma | Namespace | `edger` | `namespace.yaml` |
| Values do release | — (helm) | `edger` | `values.edger.yaml` |
| PVC do sqld | PersistentVolumeClaim | `sqld-data` | `sqld.yaml` |
| sqld | Deployment | `sqld` | `sqld.yaml` |
| Service do banco `cinzel` | Service | `cinzel` | `sqld.yaml` |
| Service do banco `pinar-prd` | Service | `pinar-prd` | `sqld.yaml` |
| Service do banco `pinar-stg` | Service | `pinar-stg` | `sqld.yaml` |
| Entrada no sqld | NetworkPolicy | `sqld-ingress` | `sqld.yaml` |
| Saída do ensaio | NetworkPolicy (egress) | `ensaio-egress` | `rehearsal/egress.yaml` |
| PVC dos backups | PersistentVolumeClaim | `sqld-backups` | `backup/pvc.yaml` |
| Script de backup | ConfigMap (gerado) | `sqld-backup-script-<hash>` | `backup/backup.sh` + `backup/kustomization.yaml` |
| Backup do banco `cinzel` | CronJob | `sqld-backup-cinzel` | `backup/cronjob-cinzel.yaml` |
| Backup do banco `pinar-prd` | CronJob | `sqld-backup-pinar-prd` | `backup/cronjob-pinar-prd.yaml` |
| Backup do banco `pinar-stg` | CronJob | `sqld-backup-pinar-stg` | `backup/cronjob-pinar-stg.yaml` |
| Rate limit do admin | Middleware (traefik.io/v1alpha1) | `edger-admin-ratelimit` | `control-plane.yaml` |
| Rota do control plane | IngressRoute (traefik.io/v1alpha1) | `edger` | `control-plane.yaml` |
| Certificado do control plane | Certificate (cert-manager.io/v1) | `edger-tls` | `control-plane.yaml` |
| Ensaio (overlay) | Kustomize | `edger-ensaio` | `rehearsal/` |
| Values do ensaio | — (helm) | `edger` | `rehearsal/values.rehearsal.yaml` |
| EdgeR (release) | ConfigMap/PVC/Service/Deployment | `edger`, `edger-user-workers`, `edger:3000` | chart `charts/edger` + `values.edger.yaml` |

Aplicação (produção): `kubectl apply -f namespace.yaml -f sqld.yaml -f
control-plane.yaml -k backup/` — ou objeto a objeto; o backup precisa do
kustomize porque gera o ConfigMap do script com hash.

## Segredos a criar

Nenhum valor vive em git. Os comandos usam caminhos de arquivo de
exemplo (`./secrets/...`) que você cria na máquina do operador e nunca
versiona. `edger-ensaio` usa os mesmos nomes, com **todos** os valores
descartáveis — a `edger-root` do ensaio é uma root key nova, gerada na
hora (ver o runbook do ensaio).

| Secret | Chave | Uso |
|---|---|---|
| `edger-root` | `root-key` | root key do EdgeR (a mesma de hoje, copiada sem imprimir) |
| `sqld-jwt` | `jwt-public-key` | chave JWT **global** do sqld — só autentica o namespace `default`; Ed25519 público, PEM (`-----BEGIN PUBLIC KEY-----`) ou base64 |
| `sqld-admin` | `admin-auth-key` | chave da API de admin (9090); vira `LIBSQL_ADMIN_AUTH_KEY` no Deployment |
| `sqld-jwt-<banco>` | `jwt-public-key` | chave JWT **do banco** (passada na criação via API de admin; guardada aqui para referência/auditoria) |
| `sqld-backup-<banco>` | `token` | token de leitura do banco para o backup (`GET /dump` com Bearer) |

```sh
# root key: copia da release atual SEM imprimir no terminal
kubectl -n cinzel get secret cinzel-edger-root \
  -o jsonpath='{.data.root-key}' | base64 -d > ./secrets/edger-root-key.txt
chmod 600 ./secrets/edger-root-key.txt
kubectl -n edger create secret generic edger-root \
  --from-file=root-key=./secrets/edger-root-key.txt

# chave JWT global do sqld (a do Cinzel atual serve; a global só cobre o
# namespace default, que fica vazio)
kubectl -n edger create secret generic sqld-jwt \
  --from-file=jwt-public-key=./secrets/sqld-jwt-public-key.pem

# chave da API de admin (aleatória, 256 bits)
openssl rand -hex 32 | tr -d '\n' > ./secrets/sqld-admin-key.txt && chmod 600 ./secrets/sqld-admin-key.txt
kubectl -n edger create secret generic sqld-admin \
  --from-file=admin-auth-key=./secrets/sqld-admin-key.txt

# por banco (cinzel, pinar-prd, pinar-stg):
# chave pública do par do banco (a PRIVADA fica no app — nunca aqui)
kubectl -n edger create secret generic sqld-jwt-cinzel \
  --from-file=jwt-public-key=./secrets/jwt-cinzel-public-key.pem
# token de leitura do banco (assinado pela privada do banco)
kubectl -n edger create secret generic sqld-backup-cinzel \
  --from-file=token=./secrets/sqld-backup-cinzel-token.txt
```

Os arquivos usados com `--from-file` não podem terminar em quebra de
linha, porque ela vai para o Secret e a API de admin do sqld passa a
recusar a chave (401). Confira com `wc -c` (64 bytes para hex de 32).
Isso aconteceu no ensaio.

## Como criar um banco

A API de admin (criar/apagar/configurar bancos) escuta em
`127.0.0.1:9090` **dentro do pod** — fora do alcance da rede do cluster
e da NetworkPolicy. Acesso por `kubectl port-forward` (ou `kubectl exec`
com `curl 127.0.0.1:9090`); confirmado no ensaio (2026-10-15):
`kubectl port-forward pod/<pod-sqld> 9090:9090` alcança o loopback do
pod, e a autenticação é `Authorization: Basic <chave crua>`
(`Bearer` dá 401; sem header, 401).

```sh
# 1. abre o loopback da API de admin
kubectl -n edger get pods -l app.kubernetes.io/name=sqld \
  -o jsonpath='{.items[0].metadata.name}'
kubectl -n edger port-forward pod/<pod-sqld> 9090:9090

# 2. cria o banco com a chave JWT dele
#    (corpo: {"jwt_key": "<chave pública Ed25519>"}; auth v0.24.32:
#    "Authorization: Basic <chave>" — o segundo token é a chave crua)
cat > ./secrets/jwt-cinzel-create.json <<'JSON'
{"jwt_key": "<chave pública Ed25519 do banco, PEM ou base64>"}
JSON
curl -fsS -X POST http://127.0.0.1:9090/v1/namespaces/cinzel/create \
  -H "Authorization: Basic $(tr -d '\n' < ./secrets/sqld-admin-key.txt)" \
  -H 'Content-Type: application/json' \
  --data @./secrets/jwt-cinzel-create.json

# 3. o Service do banco já está em sqld.yaml para cinzel, pinar-prd e
#    pinar-stg; um banco novo ganha a cópia do Service com o nome do
#    banco (mesmo seletor, porta 8080) e a linha do NetworkPolicy não
#    muda

# 4. o app aponta para o banco:
#    DATABASE_URL=http://cinzel.edger.svc.cluster.local:8080
#    token do banco, e no manifest do worker
#    allowNet: ["cinzel.edger.svc.cluster.local"]
#    (allowNet é a allowlist de egress do Deno persistente; lista
#    vazia nega rede; sem allowNet vale EDGER_DENO_ALLOW_NET/global)
```

O sqld escolhe o banco pelo primeiro rótulo do Host. Por port-forward
(restore, smoke), mande `Host: <banco>.<namespace>.svc.cluster.local`
ou o header `x-namespace-bin`. Com `127.0.0.1`, o banco vira `127` e a
resposta é 404; um Host sem ponto (`localhost`) cai no `default`. Os
tokens de um banco dão 401 nos outros bancos e no `default`.

Outros endpoints da API de admin (v0.24.32): `GET/POST
/v1/namespaces/<ns>/config` (trocar a `jwt_key`), `DELETE
/v1/namespaces/<ns>` (apagar), `POST /v1/namespaces/<ns>/fork/<to>`,
`GET /v1/namespaces/<ns>/stats`. Não existe endpoint que **liste**
namespaces (issue #1313 do libsql) — o inventário de bancos é o que
você criou e o que está em git (os Services).

## Control plane (`edger.djalmajr.dev`)

O IngressRoute `edger` encaminha **só os caminhos do control plane**
(adendo EDG-27, achado do Pinar): a rota por nome dos apps
(`edger.djalmajr.dev/<app>/...`) **não** passa por aqui, senão contorna
as proteções do ingress de cada app (o `ipAllowList` da Cloudflare do
Pinar, as exclusões de `/auth/root-token` e `/_health` do Cinzel).
Caminhos e origem no código (commit `49439fc`, base deste diretório;
`crates/edger-orchestrator/src/`):

| Caminho | Origem no código |
|---|---|
| `/` (307 → `cpanel/`) | `pipeline.rs:59` (`.route("/", get(root_redirect))`); o 307 sai do `Redirect::temporary("cpanel/")` em `pipeline.rs:136` |
| `/health` | `pipeline.rs:60`; caminho reservado em `router.rs:138` |
| `/healthz` | `pipeline.rs:61` |
| `/livez` | `pipeline.rs:62` |
| `/ready` | `pipeline.rs:65`; caminho reservado em `router.rs:139` |
| `/readyz` | `pipeline.rs:66` |
| `/metrics` | `pipeline.rs:63` (exige `observability:read`/root) |
| `/metrics/stats` | `pipeline.rs:64` |
| `/api` (exato) e `/api/*` | reserva em `router.rs:140` (`/api` ou `starts_with("/api/")`); dentro dela: `/api/admin/*` = router admin mesclado em `pipeline.rs:74` (1ª rota em `admin_api.rs:62`) e `/api/mcp` em `pipeline.rs:70` |
| `/.well-known*` | reserva em `router.rs:141` (`starts_with("/.well-known")`) |
| `/cpanel`, `/cpanel/…` | worker core `cpanel` (`workers/core/cpanel/manifest.yaml:1`), rota por nome com base em `pipeline.rs:1074` (`worker_base_path`) |
| `/webide`, `/webide/…` | worker core `webide` (`workers/core/webide/manifest.yaml:1`), idem |

A regra Traefik é estrita a propósito: `Path` exato onde o código trata
o caminho como exato e `PathPrefix` só onde o código é prefixo de
string (`/api/`, `/.well-known`, `/<worker>/`). `PathPrefix(/api)`
sozinho, por exemplo, também pegaria `/apifoo` — que no EdgeR é rota de
worker, não do control plane. O rate limit (20 req/s média, burst 100,
por IP real — Service do Traefik com `externalTrafficPolicy=Local`) é
defesa em profundidade, não controle de acesso (as chaves são de 256
bits).

## Backup

**Escolha: um CronJob por banco** (o plano deixava aberto: um job por
banco, ou um job que percorre a lista). Razões:

- parâmetros por banco ficam no manifest (não em lógica de script):
  `SQLD_URL` (Service do banco), `KEEP` (retenção), `DB_PREFIX`
  (`<banco>-*.sql.gz`), `SENTINEL` (opcional) e o Secret do token;
- a falha de um dump não bloqueia os demais bancos;
- horários e limites (`activeDeadlineSeconds`, history) independentes;
- um job único que percorre a lista concentraria os três tokens num pod
  só e precisaria de retry por banco no script — mais acoplado.

Detalhes:

- **FQDN do Service na `SQLD_URL`**: o sqld escolhe o banco pelo
  primeiro rótulo do Host; o nome curto `cinzel` (sem ponto) cairia no
  namespace `default` e o backup dumperia o banco errado.
- **Horários escalonados** (03:30 cinzel — slot atual do Cinzel; 03:40
  pinar-prd; 03:50 pinar-stg, UTC): o PVC `sqld-backups` é
  compartilhado e o local-path é single writer por nó; o escalonado
  evita três jobs escrevendo no mesmo volume ao mesmo tempo.
- **Retenção**: 14 dumps por banco (o plano fixa 14 para o Cinzel; os
  Pinar seguem o mesmo valor por coerência — **confirmar com o Pinar
  antes do corte**).
- **Sentinela**: `__migrations` para o Cinzel (o restore do Cinzel confere
  a tabela; o backup reconfere no dump); vazio para os Pinar.
- O script (`backup/backup.sh`, adaptado do do Cinzel) valida o dump
  (gzip íntegro, não vazio, `COMMIT;` final, sentinela quando
  configurada) e mantém a retenção por prefixo. O seletor de rede do
  job (`app.kubernetes.io/name: sqld-backup`) é o que a NetworkPolicy
  `sqld-ingress` reconhece.
- **Estrutura kustomize**: um arquivo de CronJob por banco com o pod
  spec comum replicado (o kustomize registra o id do recurso da base
  antes do patch renomeá-la e rejeita dois overlays partilhando a mesma
  base nomeada), e o compartilhado (PVC + ConfigMap do script) entra
  UMA vez em `backup/kustomization.yaml` — o hash do
  `configMapGenerator` é reescrito nas referências dos três CronJobs.
- **Espace do PVC**: `sqld-backups` nasce com 2Gi (o valor do backup do
  Cinzel) para **três** bancos compartilhando — os anexos ficam em
  BLOB. O ensaio mede o tamanho real dos dumps (item do plano); se
  14 × 3 dumps não couberem, crescer o PVC antes do corte.
- `activeDeadlineSeconds: 1800` e CPU de 1 (limite): no ensaio de
  2026-10-15, o backup do Cinzel (235 MB de SQL, 105 MB em gzip) levou
  cerca de 400 s com CPU de 200m, e os backups atuais de produção
  levam de 5 a 6,5 min. Os 600 s estourariam com o crescimento do
  banco, e o dump final da janela de corte é esse job.

## Ensaio (Fase 0)

Namespace descartável `edger-ensaio`, com cópias. Nada de rota pública:
smoke por `port-forward`. O Cinzel valida o ensaio (`restore-test ok` +
contagens). **Medir o tempo de cada passo** (a janela de corte é
confirmada no ensaio de 2026-10-15 (ver o resultado no fim desta
seção)) e depois apagar tudo.

Pré-requisitos: chart publicado `0.3.2-rc.19` ou mais nova (a primeira
com a EDG-24, o `release` de worker só de root — precisa estar no ar
**antes** de qualquer chave com escopo do Pinar); dump recente do
Cinzel; acesso ao nó (VPS).

1. Aplicar a plataforma de ensaio (sqld, Services, a NetworkPolicy de
   ingress do sqld, a NetworkPolicy de **egress** que fecha a saída do
   namespace e os backups — **sem** IngressRoute/Certificate):
   ```sh
   kubectl apply -k deploy/production/rehearsal
   ```
   Antes: se `sqld.yaml`/`backup/` mudaram desde a cópia, regenere
   `rehearsal/sqld.yaml` (diff citado no cabeçalho do arquivo).
2. Criar os Secrets em `edger-ensaio` com os **mesmos nomes** da
   produção e **todos** com valores descartáveis: `edger-root` — uma
   root key **nova, gerada na hora** (NUNCA a real — a real autentica
   como root na produção, e reutilizá-la no ensaio deixaria uma cópia
   temporária de credencial administrativa válida lá; o Secret some
   com o namespace na limpeza, passo 11); `sqld-jwt`, `sqld-admin`,
   `sqld-jwt-cinzel`, `sqld-backup-cinzel` — gerados para o ensaio.
   ```sh
   openssl rand -hex 32 | tr -d '\n' > ./secrets/edger-root-ensaio.txt \
     && chmod 600 ./secrets/edger-root-ensaio.txt
   kubectl -n edger-ensaio create secret generic edger-root \
     --from-file=root-key=./secrets/edger-root-ensaio.txt
   ```
   Os backups dos bancos Pinar não rodam de fato no ensaio, mas os
   Secrets precisam existir (o CronJob monta o `secretKeyRef`).
3. Instalar o EdgeR no ensaio (mesmo chart e versão do corte):
   ```sh
   helm upgrade --install edger oci://ghcr.io/djalmajr/charts/edger \
     --version <rc> -n edger-ensaio \
     -f deploy/production/rehearsal/values.rehearsal.yaml
   ```
   Esperar o pod pronto (`/ready`).
4. Copiar o PVC de workers (201 MB) no nó para o `edger-user-workers`
   do ensaio (dois PVCs local-path no mesmo nó; a cópia é no
   filesystem do nó, do diretório do PV antigo para o do novo). As
   versões copiadas carregam o `env` de produção (banco, tokens de
   email): por isso o ensaio tem a NetworkPolicy de egress (passo 1) e
   o cron desligado (`EDGER_CRON_ENABLED=false` nos values do ensaio),
   e o smoke usa a reinstalação do passo 7 — **não** uma versão
   copiada.
5. Criar o banco `cinzel` no sqld de ensaio com a `jwt_key` do ensaio
   (API de admin por port-forward — a seção "Como criar um banco" acima,
   com `-n edger-ensaio`; o caminho do port-forward na 9090 é
   confirmado na nota do início dessa seção).
6. Restore do Cinzel no banco `cinzel` (ferramenta do Cinzel,
   `bun run db:restore` do repositório do Cinzel — procedimento em
   "Restore por banco"): `bun run db:restore -- <dump> --check` no dump
   (`restore-test ok` + contagens por tabela) e depois o restore em
   `http://cinzel.edger-ensaio.svc.cluster.local:8080` com o token do
   banco — o restore é **em lotes** (o `db:restore` do Cinzel manda
   lotes de até 4 MiB, cada um numa transação; no ensaio, o dump
   inteiro numa requisição derrubou o sqld por OOM com 512Mi).
   Contagens por tabela: o `--check` dá as contagens do dump
   (tabela por tabela) e o restore devolve o total
   (`restore ok: N tables, M rows`) — as somas têm de bater.
7. Reinstalar o worker do Cinzel no EdgeR de ensaio **com o `env` do
   ensaio** — `DATABASE_URL=http://cinzel.edger-ensaio.svc.cluster.local:8080`
   (banco `cinzel` do sqld de ensaio) e o token de ensaio — como no
   corte real: instalação staged e **depois** promote (em vez de usar
   uma versão copiada, que carrega o `env` de produção; o smoke e o
   promote são da versão instalada agora). Depois do install staged,
   **aquecer a versão por pin** (invoke do control plane
   `/api/admin/workers/<nome>/invoke/<caminho>` com
   `x-edger-control-authorization: Bearer <root key>` e
   `x-edger-worker-version: <versão>`) e esperar o bundle antes do
   promote. Sem isso, as primeiras requisições estouram os 30 s e
   abrem o circuit breaker. Com o egress fechado do ensaio, o
   `deno bundle` não baixa o esbuild do npm e o EdgeR guarda um
   bundle quebrado do fallback (EDG-30); no ensaio, o egress 443 foi
   aberto por 12 s só para o pod do EdgeR, o bundle quebrado foi
   apagado, e a versão foi reciclada e aquecida. Os release tools do
   Cinzel
   (`common.py`, `kv_layout.py`, `t01_snapshot.py`) e os runbooks 10 e
   30 (port-forward em `svc/cinzel-edger`) apontam para o release
   antigo: **testar no ensaio**, porque o primeiro release depois do
   corte depende deles; atualizá-los antes do corte.
8. Smoke por `port-forward` (sem rota pública):
   ```sh
   kubectl -n edger-ensaio port-forward svc/edger 3000:3000
   ```
   login real, `/issues` autenticado, chamada MCP `get_workspace` e o
   gate de readiness privada `/_health/ready` do app (testa o banco,
   deadline 500 ms) — a URL exata (com o prefixo do worker) é conferida
   com o Cinzel neste passo.
9. Conferir no ensaio: backup do banco `cinzel` (rodar o job na mão:
   `kubectl -n edger-ensaio create job --from=cronjob/sqld-backup-cinzel
   ensaio-backup-cinzel`), espaço do PVC `sqld-backups`, a
   NetworkPolicy de ingress (curl num pod sem a label
   `sqld-backup`/`instance: edger` tem de falhar), a NetworkPolicy de
   egress (de um pod do ensaio, conexão para fora — p.ex. o Service
   `cinzel-sqld` do namespace `cinzel` ou qualquer host externo — tem
   de falhar, enquanto a conexão ao sqld do próprio namespace passa)
   e o port-forward da 9090.
10. Validação do Cinzel: `restore-test ok` + contagens por tabela de
    antes (produção) e depois (ensaio).
11. Limpeza:
    ```sh
    kubectl delete namespace edger-ensaio
    ```
    e apagar os PVs local-path que sobravem (o namespace não remove
    PVs).

### Resultado do ensaio (2026-10-15)

Tempos: plataforma em 4 s; Secrets em 4 s; helm em 20 s; cópia do PV
em 2 s (16 s com o EdgeR parado e religado); banco criado em 8 s;
restore em lotes em 56 s (60 lotes, pico de 309Mi no sqld); install
staged em 30 s; bundle real em 3,6 s (7,2 MB); promote imediato;
smoke em cerca de 3 s; limpeza em 87 s; backup em cerca de 400 s com
CPU de 200m (105 MB).

Validação do Cinzel: 46 tabelas e 14.870 linhas iguais ao `--check`;
integrity ok; `/_health/ready` 200 em 385 a 405 ms; login real e MCP
`get_workspace` ok.

Rede: o egress fechado barrou a produção e os hosts externos; o
ingress do sqld recusou pods sem rótulo; um pod novo com rótulo
permitido leva cerca de 1 s até a política valer (kube-router), e o
`backup.sh` já espera o `/health`.

## Corte (Fase 1)

Janela de 30 min de Cinzel fora do ar (uns 15 min de trabalho medidos
no ensaio; o gargalo é o dump final, que usa o job de backup com 1
CPU); o tempo conta a partir da quiescência do passo 2.

A ordem garante **um único escritor autoritativo em toda a
transição**: o writer antigo para antes do dump; o sqld antigo só para
depois do dump validado; o EdgeR novo só recebe tráfego com o antigo
inteiro para.

1. **Anúncio** ao Cinzel e ao Pinar; freeze de deploys (ninguém faz
   deploy até o fim da janela).
2. **Parar os escritores**: escalar `cinzel-edger` para 0 e **esperar
   o pod terminar** (a saída drena as requisições em voo):
   ```sh
   kubectl -n cinzel scale deploy/cinzel-edger --replicas=0
   # esperar a lista de pods esvaziar (timeout medido no ensaio):
   kubectl -n cinzel get pods -l app.kubernetes.io/instance=cinzel-edger
   ```
   O sqld antigo **continua no ar**. **Registrar o instante da
   quiescência** — ele é a marca do dump final.
3. **Dump final do sqld antigo**, com os escritores parados e o banco
   estável: dump do banco atual com `bun run db:restore -- <dump>
   --check` (`restore-test ok`) e contagens por tabela, no nó — o
   artefato só é aceito validado.
4. **Só então** escalar `cinzel-sqld` para 0
   (`kubectl -n cinzel scale deploy/cinzel-sqld --replicas=0`).
5. **Tar do PVC de workers** no nó, com o EdgeR parado e sem deploys
   (nenhuma mutação do volume durante a cópia).
6. **Dados**:
   - copiar o conteúdo do PVC de workers (201 MB, mesmo nó) para o
     `edger-user-workers` do namespace `edger`;
   - criar o banco `cinzel` no sqld novo com a `jwt_key` real do
     Cinzel (API de admin por port-forward);
   - restore do dump final em `http://cinzel.edger.svc.cluster.local:8080`
     (procedimento em "Restore por banco"), **em lotes** (o
     `db:restore` do Cinzel manda lotes de até 4 MiB, cada um numa
     transação; no ensaio, o dump inteiro numa requisição derrubou o
     sqld por OOM com 512Mi) — **sem `--baseline`** e sem `create`
     com `dump_url` (revisão do Cinzel);
   - contagens por tabela de antes e de depois (anexar à ata da
     janela).
7. **EdgeR**:
   ```sh
   helm upgrade --install edger oci://ghcr.io/djalmajr/charts/edger \
     --version <rc> -n edger -f deploy/production/values.edger.yaml \
     --history-max 5
   ```
   Esperar o pod pronto.
8. **Cinzel** (feito pelo Cinzel):
   - **primeiro** atualizar o OpenBao `secret/cinzel/prod/database`
     (`DATABASE_URL=http://cinzel.edger.svc.cluster.local:8080`,
     `DATABASE_AUTH_TOKEN` novo, `SQLD_JWT_PUBLIC_KEY` = chave pública
     do banco `cinzel` — definir o destino dela na `jwt_key` por banco
     e atualizar o runbook 60), porque o gate `t02_kv_env.py` bloqueia
     divergência entre o OpenBao e o manifesto;
   - **depois** reinstalar com URL e token novos (instalação staged,
     depois promote — antes do promote, **aquecer a versão por pin**:
     invoke do control plane
     `/api/admin/workers/<nome>/invoke/<caminho>` com
     `x-edger-control-authorization: Bearer <root key>` e
     `x-edger-worker-version: <versão>`, e esperar o bundle);
   - reinstalar também a **versão de rollback** `0.20261013.25586` com a
     URL e o token novos, staged e sem promote. As ~15 versões
     desabilitadas apontam para o banco antigo: ficam desabilitadas e
     **não podem ser reabilitadas sem reinstalação**.
9. **Rotas e TLS** em `edger`: `kubectl apply -f
   deploy/production/control-plane.yaml` (IngressRoute `edger` +
   Middleware + Certificate) e o IngressRoute de `cinzel.app` a partir
   do repositório do Cinzel (mantendo as exclusões de
   `/auth/root-token` e `/_health`, `passHostHeader: false` e o prefixo
   do worker). Os Secrets TLS (`cinzel-tls`, `edger-tls`) são **copiados**
   dos namespaces antigos para não esperar nova emissão. Depois, remover
   as rotas antigas em `cinzel`.
10. **Gate e smoke**: readiness privada `/_health/ready` (testa o banco,
   deadline 500 ms) por `port-forward` em `svc/edger`; smoke público e
   com chave (procedimento das rcs); validação do Cinzel: login real,
   `/issues` autenticado e a chamada MCP `get_workspace`. As sessões
   continuam válidas (`BETTER_AUTH_SECRET` é o mesmo e o banco foi
   copiado). **Anúncio do fim da janela.**

## Pós-corte (Fase 2), rollback e restore por banco

- O release `cinzel-edger`, o `cinzel-sqld` e os PVCs antigos ficam
  parados (0 réplicas) por **7 dias**, para rollback, como o
  `planner-edger` ficou. Depois disso, removê-los.
- **Rollback de plataforma** — em nenhum momento as duas instalações
  recebem tráfego juntas, nem dois escritores ao mesmo tempo: a nova
  para e a quiescência é atestada **antes** de qualquer dump ou
  reativação do lado antigo (barreira igual à do corte, passo 2), nos
  dois casos.
  - **Barreira de quiescência (primeiro, nos dois casos):**
    1. escalar o `edger` novo para 0
       (`kubectl -n edger scale deploy/edger --replicas=0`);
    2. **esperar a lista de pods do release ficar vazia** e registrar o
       instante (a saída drena as requisições em voo; lista já vazia =
       barreira já comprovada):
       ```sh
       kubectl -n edger wait --for=delete pod \
         -l app.kubernetes.io/instance=edger --timeout=120s
       # ou laço: kubectl -n edger get pods -l app.kubernetes.io/instance=edger
       # (timeout ajustado ao medido no ensaio)
       ```
    3. **se a barreira não se comprovar dentro do timeout: ABORTAR o
       rollback e investigar** — sem dump, sem reativação do lado
       antigo (as duas instalações ficam paradas durante a
       investigação).
  - **(a) Antes de qualquer escrita no banco novo** (só depois da
    barreira; banco antigo primeiro, writer antigo por último):
    1. escalar `cinzel-sqld` para 1 e esperar pronto (o banco antigo;
       o writer antigo segue em 0);
    2. o Cinzel volta os valores do OpenBao
       (`secret/cinzel/prod/database`) aos antigos (URL e token do
       banco antigo);
    3. remover as rotas novas em `edger` (o IngressRoute `edger` do
       control plane e o de `cinzel.app`) e reaplicar as antigas em
       `cinzel` — ainda sem writer em nenhum lado;
    4. **por último** escalar `cinzel-edger` para 1 (o writer antigo)
       e esperar pronto;
    5. readiness e smoke antes de liberar tráfego.
  - **(b) Depois de escritas no banco novo** (só depois da barreira;
    banco antigo primeiro, writer antigo por último):
    1. com o `cinzel-edger` antigo **ainda em 0**, preservar o estado
       anterior: **tar do PVC antigo (`cinzel-sqld-data`) no nó**, com
       o sqld antigo parado (mesma mecânica do tar do corte, passo 5);
    2. subir **só** o `cinzel-sqld` antigo (escalar para 1 e esperar
       pronto) — nenhum writer está no ar;
    3. preparar o banco de destino vazio no sqld antigo (a sequência
       exata de esvaziamento é conferida no ensaio);
    4. dump do banco `cinzel` novo (`GET /dump`, procedimento
       "Restore por banco"), com `restore-test ok` e contagens por
       tabela — o artefato só é aceito validado;
    5. restaurar o dump no destino com a ferramenta do Cinzel
       (`bun run db:restore`), com `restore-test ok` e contagens —
       validado antes de seguir;
    6. reverter o OpenBao aos valores antigos e trocar as rotas
       (remover as novas e reaplicar as antigas) — ainda sem writer;
    7. **por último** escalar `cinzel-edger` para 1 (o writer antigo)
       e esperar pronto;
    8. readiness e smoke antes de liberar tráfego;
    9. preservar os dois lados: o PVC antigo (com o tar do passo 1) e
       o novo, **nada apagado** (os 7 dias cobrem ambos; só remover
       depois de decidido).
- Atualizar o procedimento das rcs no ai-memory e a doc de operação do
  Cinzel; no repositório do Cinzel: os testes que fixam nomes
  (`scripts/cinzel-manifests.test.ts`), os runbooks 50 e 60, os release
  tools e os runbooks 10 e 30, o `AGENTS.md` e o checkpoint de
  produção.
- **Restore por banco** (emergência; **não para o EdgeR** — o restore de
  um app só desabilita o worker daquele app):
  1. desabilitar o worker do app
     (`POST /api/admin/workers/<app>/disable` por port-forward — o resto
     dos apps continua no ar);
  2. o restore do Cinzel **recusa destino não vazio** — esvaziar o
     banco pela API de admin: `DELETE /v1/namespaces/<banco>` e `POST
     /v1/namespaces/<banco>/create` com a `jwt_key` (a sequência exata
     é conferida no ensaio, item 5–6);
  3. `bun run db:restore <dump> --url
     http://<banco>.edger.svc.cluster.local:8080` com o token do banco
     (`DATABASE_AUTH_TOKEN` no ambiente);
  4. reabilitar o worker (`POST /api/admin/workers/<app>/enable`).

### Restore por banco (procedimento)

A ferramenta (`bun run db:restore` / `bun run db:restore-test` do
repositório do Cinzel) usa `@libsql/client`: o `Host` da URL escolhe o
banco (primeiro rótulo), o token vem de `DATABASE_AUTH_TOKEN`, o destino
não vazio é recusado, o `COMMIT;` final é validado e o `--check` confere
`integrity_check`, a migração mais recente (versus o repositório) e as
contagens por tabela. Para rodar de fora do cluster, o FQDN do Service
precisa resolver para o port-forward:

```sh
kubectl -n edger port-forward svc/cinzel 8080:8080 &
echo '127.0.0.1 cinzel.edger.svc.cluster.local' | sudo tee -a /etc/hosts
# (remover a linha do /etc/hosts depois; no macOS a entrada do
# /etc/hosts tem precedência sobre o mDNS do sufixo .local)
DATABASE_AUTH_TOKEN="<token do banco>" bun run db:restore -- <dump> --check
DATABASE_AUTH_TOKEN="<token do banco>" \
  bun run db:restore <dump> --url http://cinzel.edger.svc.cluster.local:8080
```

O `--check` não usa URL nem token (roda o SQL num banco local
efêmero): é o `restore-test` que deve imprimir `restore-test ok` antes
de qualquer restore (final do backup, ensaio, janela).

## Como um app novo entra

1. **Banco**: gerar o par de chaves Ed25519 (a privada fica no app —
   OpenBao do app, segredos do repositório do app; nunca em git nem
   neste diretório); criar o banco via API de admin com a `jwt_key`
   (seção acima); criar os Secrets `sqld-jwt-<banco>` e
   `sqld-backup-<banco>`; gerar o token de leitura do banco.
2. **Service**: cópia do Service de `sqld.yaml` com o nome do banco
   (mesmo seletor `app.kubernetes.io/name: sqld`, porta 8080) — PR neste
   repositório.
3. **Backup**: cópia de um `cronjob-<banco>.yaml` de `backup/` com
   `SQLD_URL`/`DB_PREFIX`/`KEEP`/`SENTINEL` do banco e o `secretKeyRef`
   do seu token; horário escalonado; PR neste repositório.
4. **Rota**: IngressRoute no namespace `edger` (vive no **repositório
   do app**, aplicada por ele) apontando para o Service `edger:3000`,
   com o(s) host(s) do app, `passHostHeader: false` e o middleware de
   `addPrefix` do worker (rota por nome) — ou `hosts:` no manifest do
   worker (rota por host). Certificate no `letsencrypt-dns`. **Atrás da
   Cloudflare** (Pinar): middleware `ipAllowList` restrito aos IPs da
   Cloudflare. O control plane (`edger.djalmajr.dev`) **não** é o
   caminho dos apps (adendo EDG-27).
5. **Worker**: `DATABASE_URL=http://<banco>.edger.svc.cluster.local:8080`,
   o token do banco e `allowNet` com o FQDN do banco (e só dele) no
   manifest.
6. **Escopo do EdgeR**: chave de API com escopo do app (padrão
   `pinar*` do Pinar — só com a EDG-24 já publicada; a EDG-25 não
   altera esse mecanismo).

## O que fica fora deste diretório

- Rotas e segredos dos apps: repositório do Cinzel (`deploy/k8s/`,
  OpenBao `secret/cinzel/prod/database`), repositório do Pinar.
- A versão do chart usada no corte: a publicada no ghcr (o chart deste
  repositório é a fonte; `values.edger.yaml` fixa `rc.19` ou mais nova).
- O `planner-edger` e demais releases fora da plataforma: cada um no
  seu lugar.
